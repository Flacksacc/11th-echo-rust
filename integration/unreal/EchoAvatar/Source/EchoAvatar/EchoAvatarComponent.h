#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "GameFramework/Actor.h"
#include "EchoAvatarComponent.generated.h"
// Add to the supplied character Blueprint. Animation graphs read these values.
UCLASS(ClassGroup=(Echo),meta=(BlueprintSpawnableComponent))
class ECHOAVATAR_API UEchoAvatarComponent : public UActorComponent {
 GENERATED_BODY()
public:
 UEchoAvatarComponent() { PrimaryComponentTick.bCanEverTick=true; }
 virtual void TickComponent(float Delta,ELevelTick Tick,FActorComponentTickFunction* Function) override {
  Super::TickComponent(Delta,Tick,Function);
  if(!GetOwner()) return;
  const bool Idle=AssistantState==TEXT("Ready")&&!Listening&&!Sitting;
  IdleSeconds=Idle?IdleSeconds+Delta:0;
  const float Phase=FMath::Fmod(IdleSeconds,30.f);
  Walking=Idle&&Phase>20.f&&Phase<26.f;
  if(Walking) WalkPosition=FMath::Sin((Phase-20.f)/6.f*PI)*FMath::Min(LedgeWidth*0.15f,35.f);
  else WalkPosition=FMath::FInterpTo(WalkPosition,0.f,Delta,2.f);
  Gesture=Listening?TEXT("Listen"):AssistantState.Contains(TEXT("Thinking"))?TEXT("Think"):AssistantState.Contains(TEXT("Speak"))?TEXT("Speak"):TEXT("Idle");
  const FVector Anchor=Sitting?SittingAnchor:StandingAnchor;
  GetOwner()->SetActorLocation(FVector(0,WalkPosition,0)-Anchor);
 }
 UPROPERTY(BlueprintReadOnly,Category="Echo") bool Walking=false;
 UPROPERTY(BlueprintReadOnly,Category="Echo") FName Gesture=TEXT("Idle");
 float IdleSeconds=0;
 UPROPERTY(BlueprintReadOnly,Category="Echo") FString AssistantState=TEXT("Ready");
 UPROPERTY(BlueprintReadOnly,Category="Echo") bool Listening=false;
 UPROPERTY(BlueprintReadOnly,Category="Echo") bool Sitting=false;
 UPROPERTY(BlueprintReadOnly,Category="Echo") float LedgeWidth=420;
 UPROPERTY(BlueprintReadOnly,Category="Echo") float WalkPosition=0;
 UPROPERTY(BlueprintReadOnly,Category="Echo") FName FacialSubject=TEXT("EchoSpeech");
 UPROPERTY(EditAnywhere,BlueprintReadWrite,Category="Echo") FVector StandingAnchor=FVector::ZeroVector;
 UPROPERTY(EditAnywhere,BlueprintReadWrite,Category="Echo") FVector SittingAnchor=FVector::ZeroVector;
};
